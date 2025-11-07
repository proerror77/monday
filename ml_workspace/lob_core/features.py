#!/usr/bin/env python3
"""
LOB微观结构特征工程
实现HFT级别的高频特征和DeepLOB兼容特征
"""

import numpy as np
import pandas as pd
from typing import List, Dict, Tuple, Optional, Union
from dataclasses import dataclass
from decimal import Decimal
import warnings
from scipy import stats
from scipy.signal import savgol_filter
from sklearn.preprocessing import StandardScaler, RobustScaler

from .data_structures import LOBSnapshot, LOBMessage, Trade, Side, PriceLevel


@dataclass
class FeatureConfig:
    """特征计算配置"""
    lob_depth: int = 10           # LOB深度
    time_windows: List[int] = None # 时间窗口（秒）
    volume_buckets: List[int] = None # Volume buckets
    price_precision: int = 6      # 价格精度
    enable_normalization: bool = True
    enable_technical_indicators: bool = True

    def __post_init__(self):
        if self.time_windows is None:
            self.time_windows = [1, 5, 10, 30, 60]  # 1s, 5s, 10s, 30s, 1min
        if self.volume_buckets is None:
            self.volume_buckets = [10, 50, 100, 500]  # Volume thresholds


class MicrostructureFeatures:
    """微观结构特征计算器"""

    def __init__(self, config: FeatureConfig):
        self.config = config

    def compute_spread_features(self, snapshots: List[LOBSnapshot]) -> Dict[str, np.ndarray]:
        """价差相关特征"""
        features = {}

        bid_prices = []
        ask_prices = []
        spreads = []
        relative_spreads = []

        for snap in snapshots:
            if snap.best_bid and snap.best_ask:
                bid_price = float(snap.best_bid.price)
                ask_price = float(snap.best_ask.price)
                spread = ask_price - bid_price
                mid_price = (bid_price + ask_price) / 2

                bid_prices.append(bid_price)
                ask_prices.append(ask_price)
                spreads.append(spread)
                relative_spreads.append(spread / mid_price if mid_price > 0 else 0)

        if spreads:
            features.update({
                'spread_mean': np.mean(spreads),
                'spread_std': np.std(spreads),
                'spread_relative_mean': np.mean(relative_spreads),
                'spread_relative_std': np.std(relative_spreads),
                'spread_skew': stats.skew(spreads) if len(spreads) > 2 else 0,
                'spread_kurt': stats.kurtosis(spreads) if len(spreads) > 2 else 0
            })

        return features

    def compute_depth_features(self, snapshots: List[LOBSnapshot]) -> Dict[str, np.ndarray]:
        """深度相关特征"""
        features = {}

        for depth in range(1, min(self.config.lob_depth + 1, 6)):  # Top 5 levels
            bid_volumes = []
            ask_volumes = []
            bid_prices = []
            ask_prices = []

            for snap in snapshots:
                if len(snap.bids) >= depth and len(snap.asks) >= depth:
                    bid_volumes.append(snap.bids[depth-1].size)
                    ask_volumes.append(snap.asks[depth-1].size)
                    bid_prices.append(float(snap.bids[depth-1].price))
                    ask_prices.append(float(snap.asks[depth-1].price))

            if bid_volumes and ask_volumes:
                features.update({
                    f'bid_volume_L{depth}_mean': np.mean(bid_volumes),
                    f'ask_volume_L{depth}_mean': np.mean(ask_volumes),
                    f'bid_volume_L{depth}_std': np.std(bid_volumes),
                    f'ask_volume_L{depth}_std': np.std(ask_volumes),
                    f'volume_imbalance_L{depth}': np.mean(
                        [(b - a) / (b + a + 1e-8) for b, a in zip(bid_volumes, ask_volumes)]
                    )
                })

        return features

    def compute_price_impact_features(self, snapshots: List[LOBSnapshot]) -> Dict[str, np.ndarray]:
        """价格冲击特征"""
        features = {}

        for volume_threshold in self.config.volume_buckets:
            bid_costs = []
            ask_costs = []

            for snap in snapshots:
                # 计算market order的价格冲击
                bid_cost = self._compute_market_impact(snap.bids, volume_threshold)
                ask_cost = self._compute_market_impact(snap.asks, volume_threshold)

                if bid_cost is not None:
                    bid_costs.append(bid_cost)
                if ask_cost is not None:
                    ask_costs.append(ask_cost)

            if bid_costs and ask_costs:
                features.update({
                    f'bid_impact_{volume_threshold}_mean': np.mean(bid_costs),
                    f'ask_impact_{volume_threshold}_mean': np.mean(ask_costs),
                    f'impact_asymmetry_{volume_threshold}':
                        np.mean(bid_costs) - np.mean(ask_costs),
                })

        return features

    def _compute_market_impact(self, levels: List[PriceLevel],
                              volume: int) -> Optional[float]:
        """计算market order的平均执行价格"""
        if not levels:
            return None

        total_cost = 0
        remaining_volume = volume
        total_filled = 0

        for level in levels:
            if remaining_volume <= 0:
                break

            fill_volume = min(level.size, remaining_volume)
            total_cost += fill_volume * float(level.price)
            total_filled += fill_volume
            remaining_volume -= fill_volume

        if total_filled > 0:
            return total_cost / total_filled
        return None

    def compute_queue_dynamics(self, snapshots: List[LOBSnapshot]) -> Dict[str, np.ndarray]:
        """队列动态特征"""
        features = {}

        if len(snapshots) < 2:
            return features

        queue_positions = []
        queue_movements = []

        for i in range(1, len(snapshots)):
            prev_snap = snapshots[i-1]
            curr_snap = snapshots[i]

            # 计算best bid/ask的队列位置变化
            if (prev_snap.best_bid and curr_snap.best_bid and
                prev_snap.best_ask and curr_snap.best_ask):

                bid_queue_delta = self._compute_queue_position_change(
                    prev_snap.best_bid, curr_snap.best_bid
                )
                ask_queue_delta = self._compute_queue_position_change(
                    prev_snap.best_ask, curr_snap.best_ask
                )

                if bid_queue_delta is not None:
                    queue_movements.append(bid_queue_delta)
                if ask_queue_delta is not None:
                    queue_movements.append(ask_queue_delta)

        if queue_movements:
            features.update({
                'queue_movement_mean': np.mean(queue_movements),
                'queue_movement_std': np.std(queue_movements),
                'queue_movement_skew': stats.skew(queue_movements) if len(queue_movements) > 2 else 0
            })

        return features

    def _compute_queue_position_change(self, prev_level: PriceLevel,
                                     curr_level: PriceLevel) -> Optional[float]:
        """计算队列位置变化"""
        if prev_level.price == curr_level.price:
            # 价格相同，volume变化反映队列动态
            return curr_level.size - prev_level.size
        return None

    def compute_volatility_features(self, snapshots: List[LOBSnapshot]) -> Dict[str, np.ndarray]:
        """波动率特征"""
        features = {}

        mid_prices = []
        microprices = []
        log_returns = []

        for i, snap in enumerate(snapshots):
            if snap.mid_price:
                mid_prices.append(float(snap.mid_price))
            if snap.microprice:
                microprices.append(float(snap.microprice))

            if i > 0 and len(mid_prices) >= 2:
                log_return = np.log(mid_prices[-1] / mid_prices[-2])
                log_returns.append(log_return)

        # 多时间窗口的波动率
        for window in self.config.time_windows:
            if len(log_returns) >= window:
                window_returns = log_returns[-window:]
                realized_vol = np.std(window_returns) * np.sqrt(len(window_returns))

                features.update({
                    f'realized_vol_{window}s': realized_vol,
                    f'vol_of_vol_{window}s': np.std([
                        np.std(log_returns[i:i+window//5])
                        for i in range(0, len(log_returns)-window//5, window//5)
                    ]) if window >= 5 else 0
                })

        # Parkinson估计量（使用high-low）
        if len(snapshots) >= 10:
            parkinson_vol = self._compute_parkinson_volatility(snapshots[-10:])
            features['parkinson_vol'] = parkinson_vol

        return features

    def _compute_parkinson_volatility(self, snapshots: List[LOBSnapshot]) -> float:
        """Parkinson波动率估计量"""
        highs = []
        lows = []

        for snap in snapshots:
            if snap.bids and snap.asks:
                high = float(snap.asks[0].price)  # Best ask as high
                low = float(snap.bids[0].price)   # Best bid as low
                highs.append(high)
                lows.append(low)

        if len(highs) >= 2:
            log_hl_ratios = [np.log(h/l) for h, l in zip(highs, lows) if h > 0 and l > 0]
            if log_hl_ratios:
                return np.sqrt(np.mean([x**2 for x in log_hl_ratios]) / (4 * np.log(2)))

        return 0.0


class OrderFlowFeatures:
    """订单流特征计算器"""

    def __init__(self, config: FeatureConfig):
        self.config = config

    def compute_trade_features(self, trades: List[Trade]) -> Dict[str, np.ndarray]:
        """交易流特征"""
        features = {}

        if not trades:
            return features

        # 按方向分类交易
        buy_trades = [t for t in trades if t.side == Side.BID]
        sell_trades = [t for t in trades if t.side == Side.ASK]

        # 交易量特征
        all_sizes = [t.size for t in trades]
        buy_sizes = [t.size for t in buy_trades]
        sell_sizes = [t.size for t in sell_trades]

        features.update({
            'trade_count': len(trades),
            'buy_trade_count': len(buy_trades),
            'sell_trade_count': len(sell_trades),
            'trade_size_mean': np.mean(all_sizes),
            'trade_size_std': np.std(all_sizes),
            'buy_size_mean': np.mean(buy_sizes) if buy_sizes else 0,
            'sell_size_mean': np.mean(sell_sizes) if sell_sizes else 0,
            'size_imbalance': (np.sum(buy_sizes) - np.sum(sell_sizes)) /
                            (np.sum(buy_sizes) + np.sum(sell_sizes) + 1e-8)
        })

        # 交易间隔时间
        if len(trades) >= 2:
            intervals = []
            for i in range(1, len(trades)):
                interval = (trades[i].timestamp - trades[i-1].timestamp) / 1e9  # 转为秒
                intervals.append(interval)

            features.update({
                'trade_interval_mean': np.mean(intervals),
                'trade_interval_std': np.std(intervals),
                'trade_intensity': len(trades) / (intervals[-1] if intervals else 1)
            })

        return features

    def compute_order_flow_imbalance(self, snapshots: List[LOBSnapshot],
                                   trades: List[Trade]) -> Dict[str, np.ndarray]:
        """订单流失衡指标"""
        features = {}

        if len(snapshots) < 2:
            return features

        ofi_values = []
        tick_rules = []

        for i in range(1, len(snapshots)):
            prev_snap = snapshots[i-1]
            curr_snap = snapshots[i]

            # OFI计算
            ofi = self._compute_ofi_single(prev_snap, curr_snap)
            if ofi is not None:
                ofi_values.append(ofi)

            # Tick rule
            if curr_snap.mid_price and prev_snap.mid_price:
                if curr_snap.mid_price > prev_snap.mid_price:
                    tick_rules.append(1)
                elif curr_snap.mid_price < prev_snap.mid_price:
                    tick_rules.append(-1)
                else:
                    tick_rules.append(0)

        if ofi_values:
            features.update({
                'ofi_mean': np.mean(ofi_values),
                'ofi_std': np.std(ofi_values),
                'ofi_autocorr': np.corrcoef(ofi_values[:-1], ofi_values[1:])[0,1]
                              if len(ofi_values) > 1 else 0
            })

        if tick_rules:
            features.update({
                'tick_rule_sum': np.sum(tick_rules),
                'price_trend': np.mean(tick_rules)
            })

        return features

    def _compute_ofi_single(self, prev_snap: LOBSnapshot,
                           curr_snap: LOBSnapshot) -> Optional[float]:
        """计算单个时间点的OFI"""
        if (not prev_snap.best_bid or not prev_snap.best_ask or
            not curr_snap.best_bid or not curr_snap.best_ask):
            return None

        # Bid side OFI
        bid_ofi = 0
        if prev_snap.best_bid.price == curr_snap.best_bid.price:
            bid_ofi = curr_snap.best_bid.size - prev_snap.best_bid.size
        elif curr_snap.best_bid.price > prev_snap.best_bid.price:
            bid_ofi = curr_snap.best_bid.size
        else:
            bid_ofi = -prev_snap.best_bid.size

        # Ask side OFI
        ask_ofi = 0
        if prev_snap.best_ask.price == curr_snap.best_ask.price:
            ask_ofi = curr_snap.best_ask.size - prev_snap.best_ask.size
        elif curr_snap.best_ask.price < prev_snap.best_ask.price:
            ask_ofi = curr_snap.best_ask.size
        else:
            ask_ofi = -prev_snap.best_ask.size

        return bid_ofi - ask_ofi


class DeepLOBFeatures:
    """DeepLOB兼容的特征矩阵"""

    def __init__(self, config: FeatureConfig):
        self.config = config

    def create_lob_tensor(self, snapshots: List[LOBSnapshot]) -> np.ndarray:
        """
        创建DeepLOB格式的LOB张量
        Shape: (T, 2*D) 其中T是时间步，D是深度
        每行包含: [ask_p1, ask_v1, ..., ask_pD, ask_vD, bid_p1, bid_v1, ..., bid_pD, bid_vD]
        """
        T = len(snapshots)
        D = self.config.lob_depth
        tensor = np.zeros((T, 4 * D))  # 2*D for asks, 2*D for bids

        for t, snap in enumerate(snapshots):
            # Normalize prices relative to mid price
            mid_price = float(snap.mid_price) if snap.mid_price else 1.0

            # Ask side (prices + volumes)
            for d in range(min(D, len(snap.asks))):
                level = snap.asks[d]
                tensor[t, d] = float(level.price) / mid_price  # Normalized ask price
                tensor[t, D + d] = level.size  # Ask volume

            # Bid side (prices + volumes)
            for d in range(min(D, len(snap.bids))):
                level = snap.bids[d]
                tensor[t, 2*D + d] = float(level.price) / mid_price  # Normalized bid price
                tensor[t, 3*D + d] = level.size  # Bid volume

        return tensor

    def create_feature_matrix(self, snapshots: List[LOBSnapshot],
                            include_derived: bool = True) -> np.ndarray:
        """创建包含衍生特征的矩阵"""
        base_tensor = self.create_lob_tensor(snapshots)

        if not include_derived:
            return base_tensor

        # 添加衍生特征
        derived_features = self._compute_derived_features(snapshots)

        if derived_features.size > 0:
            # 确保时间维度匹配
            if derived_features.shape[0] == base_tensor.shape[0]:
                return np.hstack([base_tensor, derived_features])
            else:
                warnings.warn("Derived features shape mismatch, using base tensor only")

        return base_tensor

    def _compute_derived_features(self, snapshots: List[LOBSnapshot]) -> np.ndarray:
        """计算衍生特征"""
        features_list = []

        for snap in snapshots:
            row_features = []

            # 价差特征
            if snap.spread:
                rel_spread = float(snap.spread) / float(snap.mid_price) if snap.mid_price else 0
                row_features.append(rel_spread)
            else:
                row_features.append(0)

            # 失衡特征（多个级别）
            for depth in range(1, min(4, self.config.lob_depth + 1)):
                if len(snap.bids) >= depth and len(snap.asks) >= depth:
                    bid_vol = snap.bids[depth-1].size
                    ask_vol = snap.asks[depth-1].size
                    imbalance = (bid_vol - ask_vol) / (bid_vol + ask_vol + 1e-8)
                    row_features.append(imbalance)
                else:
                    row_features.append(0)

            # Microprice特征
            if snap.microprice and snap.mid_price:
                microprice_diff = (float(snap.microprice) - float(snap.mid_price)) / float(snap.mid_price)
                row_features.append(microprice_diff)
            else:
                row_features.append(0)

            features_list.append(row_features)

        return np.array(features_list) if features_list else np.array([])


class FeatureEngineeringPipeline:
    """特征工程管道"""

    def __init__(self, config: FeatureConfig):
        self.config = config
        self.microstructure = MicrostructureFeatures(config)
        self.order_flow = OrderFlowFeatures(config)
        self.deeplob = DeepLOBFeatures(config)
        self.scaler = RobustScaler() if config.enable_normalization else None

    def extract_features(self, snapshots: List[LOBSnapshot],
                        trades: List[Trade]) -> Dict[str, Union[np.ndarray, Dict]]:
        """提取所有特征"""

        # 基础微观结构特征
        spread_features = self.microstructure.compute_spread_features(snapshots)
        depth_features = self.microstructure.compute_depth_features(snapshots)
        impact_features = self.microstructure.compute_price_impact_features(snapshots)
        queue_features = self.microstructure.compute_queue_dynamics(snapshots)
        vol_features = self.microstructure.compute_volatility_features(snapshots)

        # 订单流特征
        trade_features = self.order_flow.compute_trade_features(trades)
        ofi_features = self.order_flow.compute_order_flow_imbalance(snapshots, trades)

        # 队列动力学特征
        from .queue_dynamics import QueueImbalanceFeatures, AdvancedQueueAnalytics
        queue_analyzer = QueueImbalanceFeatures()
        advanced_analytics = AdvancedQueueAnalytics()

        queue_dynamic_features = {}
        if snapshots:
            # 计算队列失衡
            latest_snapshot = snapshots[-1]
            imbalance_features = queue_analyzer.compute_queue_imbalance(latest_snapshot)
            slope_features = queue_analyzer.compute_queue_slope(latest_snapshot)
            smart_money_features = queue_analyzer.compute_smart_money_indicators(snapshots[-20:])

            queue_dynamic_features.update(imbalance_features)
            queue_dynamic_features.update(slope_features)
            queue_dynamic_features.update(smart_money_features)

            # 流动性指标
            liquidity_metrics = advanced_analytics.compute_liquidity_metrics(snapshots)
            queue_dynamic_features.update(liquidity_metrics)

        # DeepLOB张量
        lob_tensor = self.deeplob.create_lob_tensor(snapshots)
        feature_matrix = self.deeplob.create_feature_matrix(snapshots, include_derived=True)

        # 合并标量特征
        scalar_features = {}
        for feature_dict in [spread_features, depth_features, impact_features,
                           queue_features, vol_features, trade_features,
                           ofi_features, queue_dynamic_features]:
            scalar_features.update(feature_dict)

        return {
            'scalar_features': scalar_features,
            'lob_tensor': lob_tensor,
            'feature_matrix': feature_matrix,
            'queue_features': queue_dynamic_features,
            'raw_snapshots_count': len(snapshots),
            'raw_trades_count': len(trades)
        }

    def normalize_features(self, features: Dict[str, Union[np.ndarray, Dict]]) -> Dict:
        """标准化特征"""
        if not self.config.enable_normalization or not self.scaler:
            return features

        normalized_features = features.copy()

        # 标准化标量特征
        if 'scalar_features' in features and features['scalar_features']:
            scalar_values = np.array(list(features['scalar_features'].values())).reshape(1, -1)
            normalized_values = self.scaler.fit_transform(scalar_values).flatten()

            normalized_scalar = {}
            for i, key in enumerate(features['scalar_features'].keys()):
                normalized_scalar[key] = normalized_values[i]

            normalized_features['scalar_features'] = normalized_scalar

        # 标准化张量特征
        if 'lob_tensor' in features:
            tensor = features['lob_tensor']
            if tensor.size > 0:
                normalized_tensor = self.scaler.fit_transform(tensor)
                normalized_features['lob_tensor'] = normalized_tensor

        return normalized_features

    def create_feature_vector(self, features: Dict[str, Union[np.ndarray, Dict]],
                            use_tensor: bool = False) -> np.ndarray:
        """创建单一特征向量"""
        if use_tensor and 'feature_matrix' in features:
            # 使用DeepLOB特征矩阵的最后一行
            matrix = features['feature_matrix']
            if matrix.size > 0:
                return matrix[-1]  # 最新时间步的特征

        # 使用标量特征
        if 'scalar_features' in features and features['scalar_features']:
            return np.array(list(features['scalar_features'].values()))

        return np.array([])


# 工具函数
def compute_feature_importance(features: np.ndarray, labels: np.ndarray) -> Dict[str, float]:
    """计算特征重要性（基于互信息）"""
    from sklearn.feature_selection import mutual_info_regression

    if features.ndim == 1:
        features = features.reshape(-1, 1)

    if features.shape[0] != len(labels):
        return {}

    mi_scores = mutual_info_regression(features, labels)

    return {
        f'feature_{i}': score for i, score in enumerate(mi_scores)
    }


def detect_regime_changes(features: np.ndarray, window_size: int = 100) -> np.ndarray:
    """检测市场制度变化"""
    if len(features) < window_size:
        return np.zeros(len(features))

    regime_changes = np.zeros(len(features))

    for i in range(window_size, len(features)):
        window1 = features[i-window_size:i-window_size//2]
        window2 = features[i-window_size//2:i]

        # Kolmogorov-Smirnov测试
        try:
            from scipy.stats import ks_2samp
            _, p_value = ks_2samp(window1.flatten(), window2.flatten())
            regime_changes[i] = 1 if p_value < 0.05 else 0
        except:
            regime_changes[i] = 0

    return regime_changes